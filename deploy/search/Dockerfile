FROM python:3.12-slim
WORKDIR /app
COPY deploy/search/requirements.txt /app/requirements.txt
RUN pip install --no-cache-dir -r requirements.txt
COPY search_app.py search_snapshot.py profile_store.py media_store.py cli.py /app/
COPY static/search /app/static/search
COPY static/bauhaus.css static/profile-view.css static/profile-view.js /app/static/
USER 65534:65534
EXPOSE 8001
CMD ["python", "search_app.py", "--host", "0.0.0.0", "--snapshot"]
